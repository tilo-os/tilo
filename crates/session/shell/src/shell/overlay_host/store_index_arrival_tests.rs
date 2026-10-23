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

#[test]
fn store_pages_join_an_open_search_when_the_store_index_lands() {
    let (woken, wakes) = mpsc::channel();
    let woken = Mutex::new(woken);
    let _wake = crate::shell::search::invocation_wake::install(Arc::new(move || {
        let _ = woken.lock().expect("wake sender lock").send(());
    }));
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
    wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the finished build wakes the loop");
    host.update(&doc, &root);
    assert_eq!(store_pages(&host), ["Spotify"]);
    assert!(!host.has_pending_visual_work());
    assert_eq!(host.palette.query(), "spotify");
    drop(host);
}
