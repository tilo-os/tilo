//! Apps on the machine that Tilo did not package, as the applications
//! provider lists them.

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
use automerge::AutoCommit;
use std::collections::BTreeMap;

fn chrome_rc() -> Chrome {
    ChromeState::new_with_handle(WorkspaceTreeHandle::new(WorkspaceTree::default()))
}

fn app(desktop_id: &str, name: &str) -> HostApp {
    HostApp {
        desktop_id: desktop_id.to_string(),
        name: name.to_string(),
        kind: desktop_entry::EntryKind::Flatpak,
        categories: Vec::new(),
        keywords: Vec::new(),
        mime_types: Vec::new(),
        terminal: false,
        icon: None,
        downloaded: None,
        window_identities: vec![desktop_id.to_string()],
    }
}

fn installed(package_id: &str, display_name: &str) -> InstalledApp {
    InstalledApp {
        package_id: package_id.into(),
        display_name: display_name.into(),
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        version: "1".into(),
        manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
        payload_root: std::path::PathBuf::from("/x"),
        has_gui: true,
        opens_extensions: Vec::new(),
        opens_mime_types: Vec::new(),
        opens_uri_schemes: Vec::new(),
        opens_secondary: false,
        opens_resource_kinds: Vec::new(),
        offers: Vec::new(),
    }
}

/// The rows `needle` finds among `host` apps on a device with
/// `installed` packages, as a page builds them.
fn host_rows(needle: &str, host: Vec<HostApp>, installed: &[InstalledApp]) -> Vec<SearchResult> {
    let provider = ApplicationsProvider::new();
    let snapshot = ApplicationsProvider::snapshot_from_parts(installed, Vec::new(), host, &[]);
    snapshot
        .index
        .search(needle, tilo_search::Page::first(snapshot.entries.len()))
        .hits
        .iter()
        .filter_map(
            |hit| match snapshot.entries.get(usize::try_from(hit.key).ok()?)? {
                ApplicationEntry::Host(app) => {
                    Some(provider.host_row(app, host_score(needle, hit.rank)))
                }
                _ => None,
            },
        )
        .collect()
}

#[test]
fn a_matching_host_app_offers_an_open_row_named_for_its_kind() {
    let out = host_rows("spot", vec![app("com.spotify.Client", "Spotify")], &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "Spotify");
    assert_eq!(out[0].id, "com.spotify.Client");
    assert_eq!(out[0].kind, ResourceKind::Application);
    assert_eq!(out[0].subtitle, None);
    assert_eq!(out[0].facts, ["Flatpak"]);
    let by_id = host_rows(
        "com.spotify",
        vec![app("com.spotify.Client", "Spotify")],
        &[],
    );
    assert_eq!(by_id.len(), 1, "the id is still searchable");
    assert!(host_rows("zzzqqq", vec![app("com.spotify.Client", "Spotify")], &[]).is_empty());
}

#[test]
fn keywords_and_categories_are_matchable() {
    let mut firefox = app("org.mozilla.firefox", "Firefox");
    firefox.keywords = vec!["Internet".into(), "WWW".into()];
    firefox.categories = vec!["Network".into(), "WebBrowser".into()];
    let by_keyword = host_rows("internet", vec![firefox.clone()], &[]);
    assert_eq!(by_keyword.len(), 1, "{by_keyword:?}");
    let by_category = host_rows("webbrowser", vec![firefox], &[]);
    assert_eq!(by_category.len(), 1, "{by_category:?}");
    assert!(
        by_keyword[0].score > by_category[0].score,
        "a declared keyword is a stronger signal than a category bucket"
    );
}

#[test]
fn an_empty_needle_lists_every_host_app() {
    let out = host_rows("", vec![app("a.b.One", "One"), app("a.b.Two", "Two")], &[]);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|r| r.score == 0));
}

#[test]
fn two_apps_of_one_name_say_which_is_which() {
    let system = HostApp {
        kind: desktop_entry::EntryKind::System,
        ..app("org.gnome.Console", "Terminal")
    };
    let flatpak = app("org.example.Terminal", "Terminal");
    let out = host_rows("terminal", vec![system, flatpak], &[]);
    let mut kinds: Vec<_> = out.iter().map(|row| row.facts.clone()).collect();
    kinds.sort();
    assert_eq!(kinds, [["Flatpak"], ["System"]]);
}

// Equal-quality matches rank the confined app first: Enter takes the top row.
#[test]
fn a_host_app_scores_below_an_equally_good_tilo_match() {
    // Different name/id tail, so this measures the scale, not shadowing.
    let host = host_rows("term", vec![app("org.x.Terminator", "Terminator")], &[]);
    let tilo = ApplicationsProvider::new()
        .results_for("term", vec![installed("so.tilo.terminal", "Terminal")]);
    assert!(
        tilo[0].score > host[0].score,
        "tilo {} must outrank host {}",
        tilo[0].score,
        host[0].score
    );
}

/// Must surface a visible error, not fail silently.
#[test]
fn a_failed_launch_pushes_an_error_notification() {
    let c = chrome_rc();
    let mut doc = AutoCommit::new();
    let out = host_rows("ghost", vec![app("so.tilo.ghost-host-app", "Ghost")], &[]);
    let row = out.first().expect("a row for the ghost app");
    assert!(c.borrow().notifications().is_empty());
    (row.on_open)(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &c,
        &ActionArgs::none(),
    )
    .expect("launching takes no arguments");
    let st = c.borrow();
    let n = st
        .notifications()
        .last()
        .expect("a failed launch must notify the user");
    assert_eq!(n.title, "Couldn't open Ghost");
    assert_eq!(n.severity, NotificationSeverity::Error);
    assert!(!n.body.is_empty());
}

/// Only an exact package identity hides a host app: names and
/// reverse-DNS tails are not enough, since different desktops ship
/// distinct apps called Terminal, Calendar and Notes.
#[test]
fn only_an_exact_tilo_package_identity_hides_the_host_row() {
    let shown = |host: &str, name: &str, package: &str, package_name: &str| {
        !host_rows(
            name,
            vec![app(host, name)],
            &[installed(package, package_name)],
        )
        .is_empty()
    };
    assert!(!shown(
        "so.tilo.terminal",
        "Terminal",
        "so.tilo.terminal",
        "Terminal"
    ));
    assert!(shown(
        "org.gnome.Console",
        "Terminal",
        "so.tilo.terminal",
        "Terminal"
    ));
    assert!(shown(
        "org.gnome.Calendar",
        "Calendar",
        "so.tilo.calendar",
        "Calendar"
    ));
    assert!(shown(
        "org.example.PrivateNotes",
        "Notes",
        "so.tilo.notes",
        "Notes"
    ));
}

/// An address resolves past the first palette page, and a host app's
/// row is found by any id its windows announce.
#[test]
fn every_host_app_is_addressable_and_opens_by_window_identity() {
    let mut apps: Vec<HostApp> = (0..200)
        .map(|at| app(&format!("org.fixture.App{at:03}"), &format!("App {at:03}")))
        .collect();
    apps[150].window_identities.push("fixture-window".into());
    let source = ApplicationsTestSource::with_host_apps(Vec::new(), Vec::new(), apps);
    let provider =
        ApplicationsProvider::with_test_source_and_preferences(source, std::rc::Rc::default());
    let chrome = chrome_rc();
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let ctx = || SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle: "",
        wanted: &[ResourceKind::Application],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };
    let row = provider
        .resolve("org.fixture.App199", ctx())
        .expect("the last host app resolves by id");
    assert_eq!(row.title, "App 199");
    let row = provider
        .app_row("fixture-window")
        .expect("found by its window");
    assert_eq!(row.id, "org.fixture.App150");
    assert!(provider.opens_app(&row, "fixture-window"));
}

/// Host apps are a search source of their own: switched off, their rows
/// leave while Tilo apps stay, and the switch is listed with the others.
#[test]
fn host_apps_switch_off_on_their_own() {
    let preferences = std::rc::Rc::new(std::cell::RefCell::new(
        crate::shell::search::SearchPreferences::default(),
    ));
    let source = ApplicationsTestSource::with_host_apps(
        vec![installed("so.tilo.notes", "Notes")],
        Vec::new(),
        vec![app("org.fixture.Calculator", "Calculator")],
    );
    let provider = std::rc::Rc::new(ApplicationsProvider::with_test_source_and_preferences(
        source,
        std::rc::Rc::clone(&preferences),
    ));
    let mut registry = crate::shell::search::SearchRegistry::with_preferences_handle(
        std::rc::Rc::clone(&preferences),
    );
    registry.register(provider.clone());
    assert!(
        registry
            .provider_catalog_handle()
            .borrow()
            .iter()
            .any(|(id, _)| id == HOST_APPS_SOURCE),
        "the host apps' switch is a search source"
    );
    let chrome = chrome_rc();
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let ctx = || SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle: "",
        wanted: &[ResourceKind::Application],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };
    assert!(provider.resolve("org.fixture.Calculator", ctx()).is_some());
    preferences
        .borrow_mut()
        .set_provider_enabled(HOST_APPS_SOURCE, false);
    assert!(provider.resolve("org.fixture.Calculator", ctx()).is_none());
    assert!(provider.app_row("org.fixture.Calculator").is_none());
    assert!(provider.resolve("so.tilo.notes", ctx()).is_some());
}
