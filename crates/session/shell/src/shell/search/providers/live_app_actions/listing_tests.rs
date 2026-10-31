//! A connection's listing of every action stays whole while searches come
//! and go: kept apart from search pages, for as long as the connection.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures own long-lived broker handles and lock guards by design"
)]
use std::rc::Rc;

use automerge::AutoCommit;
use tilo_actions::remote::{CatalogueResource, CatalogueRow, pending};

use super::tests::{chrome, exported, page};
use super::{LiveAppActionsProvider, directory_pages};
use crate::shell::collab_app_broker::ActionDirectoryRoot;

/// An enumeration of every action reads each connection's listing; the
/// palette and agents search in between. However many searches come and
/// go across many connections, every connection's listing stays whole:
/// complete, with all its rows, and never asked for again.
#[test]
fn repeated_mixed_searches_keep_every_connections_listing_complete() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("directory runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        assert_listings_survive_searches();
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the fixture, the listing and the searches between its reads"
)]
fn assert_listings_survive_searches() {
    use crate::shell::chrome_state::SearchScope;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible};
    use crate::shell::search::{QueryArgs, SearchRegistry};
    const CONNECTIONS: usize = 20;
    const SEARCHES: usize = 10;
    let chrome = chrome();
    let directory = crate::shell::collab_app_broker::tests::local_directory_fixture_with_membership(
        chrome.borrow().tree_handle(),
    );
    let broker = directory.broker.clone();
    let template = broker
        .action_directory_root_page(None, 256)
        .roots
        .into_iter()
        .next()
        .expect("the fixture's directory root");
    broker.replace_action_directory_roots_for_test(
        (1_u64..)
            .take(CONNECTIONS)
            .map(|peer| ActionDirectoryRoot {
                peer_id: peer,
                ..template.clone()
            })
            .collect(),
    );
    let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker));
    let mut registry = SearchRegistry::new();
    registry.register(provider.clone());
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let row = |peer: u64, label: String| {
        let descriptor = exported(&label, &format!("act{peer}"));
        CatalogueRow {
            provider: peer.to_string(),
            app_id: template.app_id.clone(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: descriptor.address.resource.clone(),
                kind: "fixture".into(),
                label,
                presentation: None,
            },
            action: Some(descriptor),
            has_children: false,
            visibility: None,
        }
    };
    // Answer every page asked for so far, as each connection would.
    let answer_all = |label: &dyn Fn(u64) -> String| {
        let keys: Vec<_> = provider
            .directory_pending
            .borrow()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let (sender, receiver) = pending();
            provider
                .directory_pending
                .borrow_mut()
                .get_mut(&key)
                .unwrap()
                .receiver = receiver;
            sender.complete(Ok(page(vec![row(key.0, label(key.0))], Vec::new())));
        }
    };
    let query = |needle: &str| {
        let args = QueryArgs {
            needle,
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        ActionCatalog::build_query(&registry, &args, &AllVisible, true)
    };
    let listed = |catalog: &ActionCatalog| {
        catalog
            .enumerate_rows(0..catalog.len())
            .into_iter()
            .filter(|row| row.title.starts_with("Listed by "))
            .count()
    };
    let listing_label = |peer: u64| format!("Listed by {peer}");

    // A listing asks for connections' pages a few at a time: answer them
    // as they are asked for, until it is whole.
    let settle = |needle: &str, label: &dyn Fn(u64) -> String| {
        for _ in 0..=CONNECTIONS {
            let catalog = query(needle);
            if catalog.completion().complete {
                return catalog;
            }
            answer_all(label);
        }
        panic!("the {needle:?} listing never became whole");
    };
    let cold = query("");
    assert!(
        !cold.completion().complete,
        "the listing's pages are on their way"
    );
    let whole = settle("", &listing_label);
    assert_eq!(listed(&whole), CONNECTIONS);

    for search in 0..SEARCHES {
        let needle = format!("needle{search}");
        // The search runs, and the pages it asks for arrive; whether it
        // covers every connection is the search's own bound, not the
        // listing's.
        for _ in 0..2 {
            let _ = query(&needle);
            answer_all(&|peer| format!("Found {needle} at {peer}"));
        }
        let again = query("");
        assert!(
            again.completion().complete,
            "after search {search} the listing is still whole: {:?}",
            again.completion()
        );
        assert_eq!(
            listed(&again),
            CONNECTIONS,
            "after search {search} every connection's listing keeps its rows"
        );
        assert!(
            provider
                .directory_pending
                .borrow()
                .keys()
                .all(|key| !directory_pages::is_listing(key)),
            "after search {search} no listing page is asked for again"
        );
    }
}
