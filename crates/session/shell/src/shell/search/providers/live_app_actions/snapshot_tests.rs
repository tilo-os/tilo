//! A listing reads one consistent snapshot of the live app actions: replies
//! landing and apps republishing while it is paged show up on the next
//! listing, never as a cursor gone stale mid-listing. And an app publishing
//! what it already had moves nothing.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures own long-lived broker handles and lock guards by design"
)]
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use automerge::AutoCommit;
use tilo_actions::remote::{CatalogueResource, CatalogueRow, pending};

use super::tests::{chrome, exported, local_action, page};
use super::{LiveAppActionsProvider, PROVIDER_ID};
use crate::shell::collab_app_broker::{ActionDirectoryRoot, PublishedSurface};
use crate::shell::collab_broker::CrdtRoute;
use crate::shell::search::{
    QueryIncompleteReason, ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage,
    SearchProvider, SearchResult,
};

/// A source listed beside the app actions whose every page runs `churn`:
/// what other threads do between two pages of a listing, made to happen
/// there on purpose.
struct Interloper {
    churn: Box<dyn Fn()>,
}

/// Pages the interloper answers in one listing: more than the app actions
/// take, so something happens between every two of theirs.
const INTERLOPER_PAGES: usize = 12;

impl SearchProvider for Interloper {
    fn id(&self) -> &'static str {
        "interloper"
    }
    fn name(&self) -> &'static str {
        "interloper"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, _ctx: SearchContext<'_>) -> Vec<SearchResult> {
        Vec::new()
    }
    fn pages_itself(&self) -> bool {
        true
    }
    fn query_page(
        &self,
        _ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        _budget: SearchBudget,
    ) -> SearchPage {
        (self.churn)();
        let at = cursor
            .and_then(|cursor| cursor.token.parse::<usize>().ok())
            .unwrap_or_default();
        let row = SearchResult::nullary(
            ResourceKind::Action,
            "interloper",
            format!("interloper-{at}"),
            "Interloper",
            None,
            1,
            |_, _, _| {},
        );
        let next = at.saturating_add(1);
        SearchPage::continuing(
            "interloper",
            vec![row],
            (next < INTERLOPER_PAGES).then(|| next.to_string()),
        )
    }
}

fn surface(peer: u64, app_id: &str, label: &str) -> PublishedSurface {
    PublishedSurface {
        peer_id: peer,
        app_id: app_id.to_owned(),
        route: CrdtRoute::app(app_id, "room"),
        actions: vec![local_action(label, "republished")],
        live: true,
    }
}

/// Twenty connections each list their actions. While the listing is paged,
/// their directory replies keep arriving and an app republishes, between
/// every two pages. No listing goes stale, and the listing settles complete
/// with every connection's every row.
#[test]
fn a_listing_paged_while_replies_and_republishes_arrive_is_complete_with_every_row() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("directory runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        assert_listing_settles_under_churn();
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the fixture, the churn and the listings read under it"
)]
fn assert_listing_settles_under_churn() {
    use crate::shell::chrome_state::SearchScope;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible};
    use crate::shell::search::{QueryArgs, SearchRegistry};
    const CONNECTIONS: usize = 20;
    const ROWS: usize = 4;
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
    let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(
        broker.clone(),
    ));
    let app_id = template.app_id.clone();
    let connection_page = move |peer: u64| {
        page(
            (0..ROWS)
                .map(|row| {
                    let label = format!("Listed by {peer} #{row}");
                    let descriptor = exported(&label, &format!("act{peer}-{row}"));
                    CatalogueRow {
                        provider: peer.to_string(),
                        app_id: app_id.clone(),
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
                })
                .collect(),
            Vec::new(),
        )
    };
    // What happened between two pages of the app actions, to check the
    // churn reached the listing.
    let replies = Rc::new(Cell::new(0usize));
    let republished = Rc::new(Cell::new(0usize));
    let churn = {
        let provider = Rc::clone(&provider);
        let replies = Rc::clone(&replies);
        let republished = Rc::clone(&republished);
        let app_id = template.app_id;
        move || {
            // Every reply owed arrives, as each connection answers.
            let keys: Vec<_> = provider
                .directory_pending
                .borrow()
                .keys()
                .cloned()
                .collect();
            for key in keys {
                let (sender, receiver) = pending();
                if let Some(owed) = provider.directory_pending.borrow_mut().get_mut(&key) {
                    owed.receiver = receiver;
                }
                sender.complete(Ok(connection_page(key.0)));
                replies.set(replies.get().saturating_add(1));
            }
            // An app beside them republishes: changed the first time, the
            // same surface again after.
            broker.publish_surface_for_test(surface(999, &app_id, "Republished"));
            republished.set(republished.get().saturating_add(1));
        }
    };
    let mut registry = SearchRegistry::new();
    registry.register(provider.clone());
    registry.register(Rc::new(Interloper {
        churn: Box::new(churn),
    }));
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let query = || {
        let args = QueryArgs {
            needle: "",
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
    // The local index builds on its own thread; between two listings, take
    // in a build that one started, as the next frame would.
    let land_index = || {
        let worker = provider.surface_build.borrow_mut().take();
        if let Some(worker) = worker {
            let index = worker.recv().expect("the index worker answers");
            *provider.surface_snapshot.borrow_mut() = Some(Rc::new(index));
        }
    };
    let went_stale: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut whole = None;
    for listing in 0..=CONNECTIONS + 3 {
        let catalog = query();
        for incomplete in &catalog.completion().incomplete {
            if incomplete.reason == QueryIncompleteReason::StaleCursor {
                went_stale
                    .borrow_mut()
                    .push(format!("listing {listing}: {}", incomplete.provider));
            }
        }
        if catalog.completion().complete {
            whole = Some(catalog);
            break;
        }
        land_index();
    }
    assert!(
        went_stale.borrow().is_empty(),
        "no listing goes stale while it is paged: {:?}",
        went_stale.borrow()
    );
    let settled = whole.expect("the listing settles complete");
    assert_eq!(
        listed(&settled),
        CONNECTIONS * ROWS,
        "every connection's every row"
    );
    assert!(
        replies.get() >= CONNECTIONS,
        "replies arrived between pages ({})",
        replies.get()
    );
    assert!(
        republished.get() > 0,
        "an app republished between pages ({})",
        republished.get()
    );
    assert!(
        settled
            .completion()
            .incomplete
            .iter()
            .all(|incomplete| incomplete.provider != PROVIDER_ID)
    );
}

/// An app publishing the surface it already had moves nothing: the
/// surfaces' generation every listing and palette reads stays, so nothing
/// reads the app actions again for it. A changed surface moves it.
#[test]
fn an_unchanged_republication_moves_nothing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("directory runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let chrome = chrome();
        let directory =
            crate::shell::collab_app_broker::tests::local_directory_fixture_with_membership(
                chrome.borrow().tree_handle(),
            );
        let broker = directory.broker.clone();
        let before = broker.surfaces_generation();
        broker.publish_surface_for_test(surface(999, "so.tilo.fixture", "First"));
        let published = broker.surfaces_generation();
        assert_ne!(published, before, "a new surface moves the generation");
        for _ in 0..3 {
            broker.publish_surface_for_test(surface(999, "so.tilo.fixture", "First"));
            assert_eq!(
                broker.surfaces_generation(),
                published,
                "the same surface again moves nothing"
            );
        }
        broker.publish_surface_for_test(surface(999, "so.tilo.fixture", "Second"));
        assert_ne!(
            broker.surfaces_generation(),
            published,
            "a changed surface moves the generation"
        );
    });
}
