//! A search typed before the Store's index is built: its pages join the
//! open palette when the index lands, with nothing more typed, and a still
//! capture waits for them.

use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};

use automerge::AutoCommit;
use ui::widget::Container;
use vello::kurbo::Rect;

use super::*;
use crate::shell::catalogue::SharedCatalogue;
use crate::shell::chrome_state::ChromeState;
use crate::shell::search::providers::StoreCatalogueProvider;
use crate::shell::search::providers::store_catalogue::{
    StoreCatalogueTestSource, StoreCatalogueTestState,
};
use crate::shell::state::WorkspaceTree;

const SPOTIFY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.16" origin="flathub">
  <component type="desktop-application">
    <id>com.spotify.Client</id>
    <name>Spotify</name>
    <summary>Online music streaming service</summary>
    <bundle type="flatpak">app/com.spotify.Client/x86_64/stable</bundle>
  </component>
</components>"#;

fn flathub_with_spotify() -> StoreCatalogueTestState {
    let source = app_catalogue::appstream::AppstreamSource {
        remote: "flathub".into(),
        installation: app_catalogue::Installation::System,
        file: "/var/lib/flatpak/appstream/flathub/x86_64/active/appstream.xml".into(),
        icons: "/var/lib/flatpak/appstream/flathub/x86_64/active/icons".into(),
    };
    StoreCatalogueTestState {
        installed: Vec::new(),
        tilo: Vec::new(),
        shared: SharedCatalogue {
            flathub: Arc::new(
                app_catalogue::appstream::parse(SPOTIFY.as_bytes(), &source, "en")
                    .expect("the Flathub catalogue parses"),
            ),
            blocks: Arc::new(app_catalogue::BlockList::empty()),
        },
    }
}

fn store_pages(host: &ShellOverlayHost) -> Vec<String> {
    let heading = crate::shell::search::in_the_store().localized_label();
    host.palette
        .visible_entries()
        .iter()
        .filter(|entry| entry.group.as_deref() == Some(heading.as_str()))
        .map(|entry| entry.label.clone())
        .collect()
}

/// The app-icon resolver is the process's one, with one wake: tests that
/// wait on it take turns.
static ICON_WAKE: Mutex<()> = Mutex::new(());

/// The loop's wake, as the shell installs it for search and app icons: each
/// wake is one message.
fn wake_channel() -> (
    crate::shell::search::invocation_wake::Installed,
    std::sync::MutexGuard<'static, ()>,
    mpsc::Receiver<()>,
) {
    let turn = ICON_WAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (woken, wakes) = mpsc::channel();
    let woken = Mutex::new(woken);
    let wake: crate::session_runtime::Wake = Arc::new(move || {
        let _ = woken.lock().expect("wake sender lock").send(());
    });
    crate::app_icons::set_wake(Arc::clone(&wake));
    (
        crate::shell::search::invocation_wake::install(wake),
        turn,
        wakes,
    )
}

/// Run the loop as the shell does: on each wake, update, until nothing
/// the frame owes is still on its way (the index, then the rows' icons).
fn settle_on_wakes(host: &mut ShellOverlayHost, wakes: &mpsc::Receiver<()>, doc: &AutoCommit) {
    loop {
        wakes
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("work on its way wakes the loop when it lands");
        host.update(doc, &automerge::ROOT);
        if !host.has_pending_visual_work() {
            return;
        }
    }
}

#[test]
fn store_pages_join_an_open_search_when_the_store_index_lands() {
    let (_wake, _turn, wakes) = wake_channel();
    let source = StoreCatalogueTestSource::new(flathub_with_spotify());
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
    let mut host =
        ShellOverlayHost::with_fixture_content(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.search_registry
        .register(Rc::new(StoreCatalogueProvider::with_test_source(
            Arc::clone(&source),
        )));
    let doc = AutoCommit::new();
    let root = automerge::ROOT;

    let building = source.hold();
    host.open_global_search(&doc, &root);
    host.palette.set_query("spotify");
    host.refresh_search_results(&doc, &root);
    host.update(&doc, &root);
    assert!(
        store_pages(&host).is_empty(),
        "the index is still being built"
    );
    assert!(
        host.has_pending_visual_work(),
        "a still capture waits for the rows the index owes"
    );

    drop(building);
    settle_on_wakes(&mut host, &wakes, &doc);
    assert_eq!(store_pages(&host), ["Spotify"]);
    assert_eq!(host.palette.query(), "spotify");
    drop(host);
}

#[test]
fn store_pages_are_under_the_apps_chip() {
    let (_wake, _turn, wakes) = wake_channel();
    let source = StoreCatalogueTestSource::new(flathub_with_spotify());
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
    let mut host =
        ShellOverlayHost::with_fixture_content(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.search_registry
        .register(Rc::new(StoreCatalogueProvider::with_test_source(
            Arc::clone(&source),
        )));
    let doc = AutoCommit::new();
    let root = automerge::ROOT;

    let building = source.hold();
    host.open_global_search(&doc, &root);
    host.palette.set_query("spotify");
    host.refresh_search_results(&doc, &root);
    host.update(&doc, &root);
    drop(building);
    settle_on_wakes(&mut host, &wakes, &doc);
    assert_eq!(store_pages(&host), ["Spotify"]);
    assert_eq!(
        host.present_kinds,
        [crate::shell::search::ResourceKind::Application],
        "the Store section is counted under the Apps chip, not a chip of its own"
    );

    host.filter_palette_to_apps_for_shots();
    host.refresh_search_results(&doc, &root);
    host.update(&doc, &root);
    assert_eq!(
        store_pages(&host),
        ["Spotify"],
        "choosing Apps keeps the Store section"
    );
    drop(host);
}
