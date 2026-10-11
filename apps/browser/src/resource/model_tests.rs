//! The browser's document on its own: bookmarks, preferences and site
//! exceptions through the same door the host uses, merging, and history.

use super::*;
use crate::library::Bookmarks;
use ui::tilo_resource::{Edit, Replica, Resource as _};

fn edit(browser: &mut Replica<Browser>, edit: &Edit<Browser>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    browser
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn bookmark(browser: &mut Replica<Browser>, url: &str, title: Option<&str>) -> Result<(), String> {
    browser
        .apply(Browser::ADD_BOOKMARK.with(browser::AddBookmark {
            url: url.into(),
            title: title.map(str::to_owned),
        }))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn listed(browser: &Replica<Browser>) -> Vec<(String, String)> {
    Bookmarks::of(&browser.state().unwrap().bookmarks)
        .entries()
        .iter()
        .map(|entry| (entry.url.clone(), entry.title.clone()))
        .collect()
}

fn id_of(browser: &Replica<Browser>, url: &str) -> Id {
    browser
        .state()
        .unwrap()
        .bookmarks
        .iter()
        .find(|(_, bookmark)| bookmark.url == url)
        .map(|(id, _)| id.clone())
        .unwrap()
}

/// A bookmark keeps the normalized address and a title; a page is
/// bookmarked once; its title is a field edit; a bad address is refused.
#[test]
fn bookmarks_are_normalized_pages_with_titles() {
    let mut browser = Replica::<Browser>::new();
    bookmark(&mut browser, " https://example.org ", None).unwrap();
    bookmark(&mut browser, "https://rust-lang.org", Some(" Rust ")).unwrap();
    bookmark(&mut browser, "https://example.org/", Some("Again")).unwrap();
    assert_eq!(
        listed(&browser),
        [
            ("https://example.org/".into(), "https://example.org/".into()),
            ("https://rust-lang.org/".into(), "Rust".into()),
        ]
    );
    for refused in ["not a url", "javascript:alert(1)", "file:///etc/passwd"] {
        assert!(bookmark(&mut browser, refused, None).is_err(), "{refused}");
    }

    let rust = id_of(&browser, "https://rust-lang.org/");
    let paths = Browser::paths().bookmarks.item(&rust);
    edit(&mut browser, &paths.title.replace("The Rust language")).unwrap();
    assert!(
        edit(&mut browser, &paths.title.replace("t".repeat(5000))).is_err(),
        "a title fits a row"
    );
    assert!(
        edit(&mut browser, &paths.url.replace("https://elsewhere.test/")).is_err(),
        "a bookmark's address is the page it was made for"
    );
    assert_eq!(listed(&browser)[1].1, "The Rust language");
    browser.apply(Browser::REMOVE_BOOKMARK.with(rust)).unwrap();
    assert_eq!(listed(&browser).len(), 1);
}

/// Preferences are field edits held to their invariant; site protection is
/// a verb that keeps exact origins.
#[test]
fn preferences_and_site_protection() {
    let mut browser = Replica::<Browser>::new();
    let preferences = Browser::paths().preferences;
    edit(&mut browser, &preferences.clear_on_exit.set(true)).unwrap();
    edit(
        &mut browser,
        &preferences.search_engine.replace("https://search.test/?q="),
    )
    .unwrap();
    assert!(edit(&mut browser, &preferences.search_engine.replace("nope")).is_err());
    let state = browser.state().unwrap();
    assert!(state.preferences.clear_on_exit);
    assert_eq!(state.preferences.search_engine, "https://search.test/?q=");

    let protect = |origin: &str, enabled: bool| {
        Browser::SET_SITE_PROTECTION.with(browser::SetSiteProtection {
            origin: origin.into(),
            enabled,
        })
    };
    browser
        .apply(protect("https://Ads.example/path?x", false))
        .unwrap();
    assert!(browser.apply(protect("mailto:a@b", false)).is_err());
    assert!(
        edit(
            &mut browser,
            &Browser::paths()
                .site_exceptions
                .insert(&"https://other.test".to_owned())
        )
        .is_err(),
        "only the verb changes the exceptions"
    );
    assert_eq!(
        browser
            .state()
            .unwrap()
            .exceptions()
            .into_iter()
            .collect::<Vec<_>>(),
        ["https://ads.example"]
    );
    browser.apply(protect("https://ads.example", true)).unwrap();
    assert!(browser.state().unwrap().site_exceptions.is_empty());
}

/// Two people bookmarking at once keep both; one page bookmarked by both
/// is listed once and removed whole; removing while someone else adds the
/// page keeps their bookmark; preferences merge per field.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Browser>::new();
    let mut theirs = ours.fork();

    bookmark(&mut ours, "https://one.test/", None).unwrap();
    bookmark(&mut theirs, "https://two.test/", None).unwrap();
    bookmark(&mut ours, "https://both.test/", Some("Ours")).unwrap();
    bookmark(&mut theirs, "https://both.test/", Some("Theirs")).unwrap();
    bookmark(&mut ours, "https://shared.test/", Some("Shared")).unwrap();
    let shared = id_of(&ours, "https://shared.test/");
    ours.apply(Browser::REMOVE_BOOKMARK.with(shared)).unwrap();
    bookmark(&mut theirs, "https://shared.test/", None).unwrap();
    edit(
        &mut ours,
        &Browser::paths().preferences.adblock_enabled.set(false),
    )
    .unwrap();
    edit(
        &mut theirs,
        &Browser::paths()
            .preferences
            .homepage
            .replace("https://home.test/"),
    )
    .unwrap();

    ours.sync_with(&mut theirs).unwrap();
    assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
    let urls: Vec<String> = listed(&ours).into_iter().map(|(url, _)| url).collect();
    assert_eq!(
        urls,
        [
            "https://both.test/",
            "https://one.test/",
            "https://shared.test/",
            "https://two.test/"
        ],
        "a page both bookmarked is listed once; a concurrent add survives a removal"
    );
    assert_eq!(
        ours.state().unwrap().bookmarks.len(),
        5,
        "both people's bookmark of one page are kept"
    );
    let both = id_of(&ours, "https://both.test/");
    ours.apply(Browser::REMOVE_BOOKMARK.with(both)).unwrap();
    assert!(
        !Bookmarks::of(&ours.state().unwrap().bookmarks).contains("https://both.test/"),
        "removing the page removes every bookmark of it"
    );
    let preferences = ours.state().unwrap().preferences;
    assert!(!preferences.adblock_enabled);
    assert_eq!(preferences.homepage, "https://home.test/");
}

/// The person's address book keeps every profile valid and within its
/// limit, whichever way a change arrives.
#[test]
fn the_address_book_holds_valid_profiles_only() {
    use crate::autofill::{AddressBook, tests::profile};

    let mut book = Replica::<AddressBook>::new();
    let paths = AddressBook::paths();
    let add = |book: &mut Replica<AddressBook>, edit: Edit<AddressBook>| {
        book.apply(Invoke::edit(&edit).unwrap()).map(drop)
    };
    add(&mut book, paths.enabled.set(true)).unwrap();
    add(&mut book, paths.profiles.insert(&profile("home"))).unwrap();
    let mut invalid = profile("bad id!");
    invalid.label = "Work".into();
    assert!(add(&mut book, paths.profiles.insert(&invalid)).is_err());
    let city = paths.profiles.item(&Id::from_raw("home")).city;
    add(&mut book, city.replace("Elsewhere")).unwrap();
    assert!(
        add(&mut book, city.replace("x".repeat(300))).is_err(),
        "a field edit is held to the profile's limits"
    );
    for index in 1..16 {
        add(
            &mut book,
            paths.profiles.insert(&profile(&format!("p{index}"))),
        )
        .unwrap();
    }
    assert!(add(&mut book, paths.profiles.insert(&profile("overflow"))).is_err());
    let state = book.state().unwrap();
    assert_eq!(state.profiles.len(), 16);
    assert_eq!(
        state
            .profiles
            .find(&Id::from_raw("home"))
            .map(|home| home.city.as_str()),
        Some("Elsewhere")
    );
}

/// An agent bookmarks, edits and protects with one call each, and the
/// person reads what it did in the browser's own sentences.
#[test]
fn an_agents_changes_render_as_the_browsers_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = ui::framework::testing::TestApp::<BrowserShell>::capture(None).unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Browser::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "add-bookmark",
        serde_json::json!({ "url": "https://rust-lang.org/", "title": "Rust" }),
    )
    .unwrap();
    let rust = app
        .state()
        .bookmarks
        .ids()
        .next()
        .cloned()
        .expect("a bookmark");
    let preferences = Browser::paths().preferences;
    for change in [
        Browser::paths()
            .bookmarks
            .item(&rust)
            .title
            .replace("Rust language"),
        preferences.adblock_enabled.set(false),
        preferences.homepage.replace("https://home.test/"),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": change }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        kind,
        "set-site-protection",
        serde_json::json!({ "origin": "https://ads.example", "enabled": false }),
    )
    .unwrap();
    app.call_by(
        agent,
        &bookmark_resource_id(&rust),
        "remove-bookmark",
        serde_json::json!({}),
    )
    .unwrap();

    let catalogue = tilo_history::Catalogue::new("en-US", &[LOCALES[0].1]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let entries: Vec<_> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
    assert_eq!(
        sentences,
        [
            "Bookmarked https://rust-lang.org/",
            "Renamed the bookmark Rust",
            "Turned ad and tracker blocking off",
            "Changed the homepage",
            "Turned tracker protection off for https://ads.example",
            "Removed the bookmark Rust language",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada"))
    );
}
