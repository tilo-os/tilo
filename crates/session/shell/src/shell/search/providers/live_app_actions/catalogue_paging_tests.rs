//! An app's directory answers the palette in pages that arrive while the
//! person types. Every page that lands is listed, whichever query or
//! continuation asked for it.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each test holds the chrome it queries for its whole short scenario"
)]

use std::rc::Rc;

use automerge::AutoCommit;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueResource, CatalogueRow, Completer, pending,
};

use super::{DirectoryCacheKey, LiveAppActionsProvider, PendingDirectoryPage};
use crate::shell::chrome_state::{Chrome, ChromeState, SearchResourceScope, SearchScope};
use crate::shell::collab_broker_capnp::ActionDirectoryRoot;
use crate::shell::search::{
    QueryArgs, ResourceKind, SearchBudget, SearchContext, SearchProvider as _, SearchRegistry,
};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};

const APP: &str = "so.tilo.remote";
const PEER: u64 = 9;

type Reply = Completer<Result<CataloguePage, ActionHostError>>;

struct Fixture {
    chrome: Chrome,
    provider: Rc<LiveAppActionsProvider>,
    actor: String,
}

fn fixture() -> Fixture {
    let mut tree = WorkspaceTree::default();
    tree.desktop
        .workspaces
        .push(Workspace::new(WorkspaceId(1), "Catalogue paging fixture"));
    tree.desktop.active = WorkspaceId(1);
    let chrome = ChromeState::new_with_handle(WorkspaceTreeHandle::new(tree));
    let broker =
        crate::shell::broker_services::test_broker_with_membership(chrome.borrow().tree_handle());
    let (cluster_id, actor) = {
        let state = chrome.borrow();
        let room = state.tree().desktop.workspaces[0].room();
        (room.to_string(), state.local_user_for_room(&room))
    };
    broker.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
        peer_id: PEER,
        app_id: APP.into(),
        cluster_id,
        record_id: "root".into(),
    }]);
    Fixture {
        chrome,
        provider: Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker)),
        actor,
    }
}

fn row(action: &str) -> CatalogueRow {
    CatalogueRow {
        provider: PEER.to_string(),
        app_id: APP.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: format!("resource:{action}"),
            kind: "fixture".into(),
            label: "Fixture".into(),
            presentation: None,
        },
        action: Some(tilo_actions::ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(format!("resource:{action}"), action),
            resource_kind: "fixture".into(),
            resource_label: "Fixture".into(),
            action_label: format!("Open {action}"),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn page(actions: &[&str], next_cursor: &[u8]) -> CataloguePage {
    CataloguePage {
        revision: "r1".into(),
        exhausted: next_cursor.is_empty(),
        budget_exhausted: !next_cursor.is_empty(),
        rows: actions.iter().map(|action| row(action)).collect(),
        next_cursor: next_cursor.to_vec(),
        ..CataloguePage::default()
    }
}

impl Fixture {
    fn key(&self, needle: &str, cursor: &[u8]) -> DirectoryCacheKey {
        (PEER, self.actor.clone(), needle.into(), cursor.to_vec())
    }

    /// The app's reply to the page at `cursor`, still owed: the provider
    /// asks no further for it and lists it when the test answers.
    fn owe(&self, needle: &str, cursor: &[u8]) -> Reply {
        let (reply, receiver) = pending();
        self.provider.directory_pending.borrow_mut().insert(
            self.key(needle, cursor),
            PendingDirectoryPage {
                receiver,
                deadline: tilo_test_env::hang_deadline(),
                initial_page: cursor.is_empty(),
            },
        );
        reply
    }
}

fn titles(rows: impl IntoIterator<Item = crate::shell::search::SearchResult>) -> Vec<String> {
    rows.into_iter().map(|row| row.title).collect()
}

/// A walk that follows the provider's cursor while the next page is owed
/// resumes at that page once it lands, not at the row count of the page
/// before it.
#[test]
fn a_continuation_that_lands_between_two_pages_is_listed_whole() {
    let fixture = fixture();
    let state = fixture.chrome.borrow();
    let doc = AutoCommit::new();
    let ctx = || SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle: "open",
        wanted: &[ResourceKind::Action],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };
    fixture
        .provider
        .directory_pages_mut()
        .insert(fixture.key("open", b""), page(&["first"], b"c1"));
    let second = fixture.owe("open", b"c1");

    let first = fixture
        .provider
        .query_page(ctx(), None, SearchBudget::PALETTE);
    assert_eq!(titles(first.results), ["Open first"]);
    assert!(!first.exhausted, "the owed page still follows");

    second.complete(Ok(page(&["second", "third"], b"")));
    let rest =
        fixture
            .provider
            .query_page(ctx(), first.next_cursor.as_ref(), SearchBudget::PALETTE);
    assert!(!rest.stale_cursor);
    assert_eq!(titles(rest.results), ["Open second", "Open third"]);
}

/// The palette scoped to one of an app's resources, typed into while the
/// app is slow to answer: every keystroke abandons the last query's pages,
/// and each page of the final query's chain shows up as it lands, without
/// another keystroke.
#[test]
fn an_open_scoped_palette_shows_each_page_as_it_lands() {
    let fixture = fixture();
    let cluster_id = fixture.chrome.borrow().tree().desktop.workspaces[0]
        .room()
        .to_string();
    fixture.chrome.borrow_mut().search_resource_scope = Some(SearchResourceScope {
        peer_id: PEER,
        cluster_id,
        app_id: APP.into(),
        resource: "folder".into(),
        label: "Folder".into(),
        kind: "fixture".into(),
    });
    let mut registry = SearchRegistry::new();
    registry.register(fixture.provider.clone());
    let state = fixture.chrome.borrow();
    let doc = AutoCommit::new();
    let args = |needle| QueryArgs {
        needle,
        scope: SearchScope::All,
        active_provider_id: None,
        wanted_kinds: &[],
        chrome: &state,
        doc: &doc,
        doc_root: &automerge::ROOT,
    };

    // Each keystroke asks the app for its query's first page; the test
    // answers in its place.
    let mut earlier: Option<Reply> = None;
    for needle in ["o", "op", "ope", "open"] {
        registry.refresh_indexes(&args(needle));
        assert!(registry.query(&args(needle)).is_empty());
        if let Some(earlier) = earlier.replace(fixture.owe(needle, b"")) {
            assert!(
                earlier.is_abandoned(),
                "a keystroke abandons the last query"
            );
        }
    }
    let first = earlier.expect("the final query's first page");

    let chain: [(&[u8], &[&str], &[u8]); 3] = [
        (b"", &["alpha"], b"c1"),
        (b"c1", &["beta"], b"c2"),
        (b"c2", &["gamma"], b""),
    ];
    let mut reply = Some(first);
    let mut shown: Vec<String> = Vec::new();
    for (cursor, actions, next) in chain {
        let landing = reply.take().expect("page in flight");
        if !next.is_empty() {
            // Owed before this page lands, so the palette's request for it
            // finds it in flight.
            reply = Some(fixture.owe("open", next));
        }
        landing.complete(Ok(page(actions, next)));
        assert!(
            registry.refresh_indexes(&args("open")),
            "the page at {cursor:?} landed: the open palette must rebuild its rows"
        );
        shown.extend(actions.iter().map(|action| format!("Open {action}")));
        let mut listed = titles(registry.query(&args("open")));
        listed.sort();
        assert_eq!(listed, shown, "after the page at {cursor:?}");
    }
}
