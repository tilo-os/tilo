//! How live app action rows read and in which order equal ones come.
use automerge::AutoCommit;

use super::{LiveAppActionsProvider, PROVIDER_ID, row_facts};
use crate::shell::chrome_state::{ChromeState, SearchScope};
use crate::shell::collab_broker::CrdtRoute;
use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};
use crate::shell::search::{ResourceKind, SearchBudget, SearchContext, SearchProvider as _};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};

const APP: &str = "so.tilo.notes";

#[test]
fn a_row_names_its_record_once() {
    assert_eq!(row_facts("Rename Trip plan", "Trip plan", APP), ["Notes"]);
    assert_eq!(row_facts("Pin trip plan", "Trip plan", APP), ["Notes"]);
    assert_eq!(
        row_facts("Delete", "Trip plan", APP),
        ["Trip plan", "Notes"],
        "a label that does not name the record keeps it on the second line"
    );
}

/// Equal matches come in title order whatever order the broker lists its
/// surfaces in, so the rows never reshuffle between one search and the next.
#[test]
fn equal_rows_come_in_title_order() {
    let mut tree = WorkspaceTree::default();
    tree.desktop
        .workspaces
        .push(Workspace::new(WorkspaceId(1), "Row order fixture"));
    tree.desktop.active = WorkspaceId(1);
    let chrome = ChromeState::new_with_handle(WorkspaceTreeHandle::new(tree));
    let room = chrome.borrow().tree().desktop.workspaces[0].room();
    let user = chrome.borrow().local_user_for_room(&room);
    chrome.borrow().tree_mut().desktop.workspaces[0].set_owner(user);
    let broker =
        crate::shell::broker_services::test_broker_with_membership(chrome.borrow().tree_handle());
    // Same length, so each matches "rename" exactly as well as the others.
    let pages = ["F", "C", "H", "A", "I", "D", "B", "G", "E"];
    broker.replace_published_surfaces_for_test(
        pages
            .iter()
            .zip(7_u64..)
            .map(|(page, peer_id)| PublishedSurface {
                peer_id,
                app_id: APP.into(),
                route: CrdtRoute::record(APP, room.to_string(), format!("page-{page}")),
                actions: vec![PublishedAction {
                    semantic_identity: None,
                    resource: format!("note:{page}"),
                    action: "rename".into(),
                    resource_kind: "note".into(),
                    resource_label: format!("Page {page}"),
                    action_label: format!("Rename Page {page}"),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::default(),
                    view: None,
                    input: PublishedInput::None,
                }],
                // Open and closed pages alike.
                live: peer_id % 2 == 0,
            })
            .collect(),
    );
    let provider = LiveAppActionsProvider::new(broker.clone());
    let state = chrome.borrow();
    let doc = AutoCommit::new();
    let ctx = SearchContext {
        query_scope: Some(SearchScope::All),
        include_programmatic: false,
        needle: "rename",
        wanted: &[ResourceKind::Action],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };
    *provider.surface_snapshot.borrow_mut() = Some(LiveAppActionsProvider::build_local_snapshot(
        &broker,
        &LiveAppActionsProvider::build_scope(&ctx),
        &provider.build_probe,
    ));
    drop(broker);
    let page = provider.query_page(ctx, None, SearchBudget::PALETTE);
    drop(provider);
    let rows: Vec<_> = page
        .results
        .iter()
        .filter(|row| row.provider_id == PROVIDER_ID && row.id.starts_with(APP))
        .collect();
    assert!(
        rows.windows(2).all(|pair| pair[0].score == pair[1].score),
        "the fixture's rows tie"
    );
    let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
    let mut sorted = titles.clone();
    sorted.sort_unstable();
    assert_eq!(titles.len(), pages.len());
    assert_eq!(titles, sorted, "tied rows come in title order");
}
